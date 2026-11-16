//! Only the principal that joined a subscription may stream, seek or LEAVE
//! it, in process (#5494). The joiner is the JOIN's subject, tenant and
//! subject type; its token scopes are not part of it. Another principal is
//! refused with `403 SubscriptionNotOwned` right after the tenant gate: before
//! the cursor check and the stream lease, with no PDP call beyond the gate and
//! no scope evaluated. A read keeps the tenant gate alone. Each test records
//! every request the PDP sees after its setup, so the list holds exactly what
//! the call under test asks. The REST entry points are covered in
//! `api/rest/handlers/delivery/`, the in-process SDK client in
//! `local_broker_tests.rs`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use authz_resolver_sdk::{
    AccessRequest, BarrierMode as PdpBarrierMode, EvaluationRequest, PolicyEnforcer, ResourceType,
    TenantMode,
};
use gts::GtsIdPattern;
use serde_json::{Value, json};
use toolkit_gts::{GtsInstanceId, gts_id};
use toolkit_security::{SecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::delivery::{JoinRequest, Position, SeekTarget};
use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::model::{
    BarrierMode, ConsumerGroupCreateInput, Interest, Joiner, OwnershipFilter,
};
use crate::domain::repo::CursorRepo;
use crate::domain::streaming::lease::StreamLeases;
use crate::domain::streaming::session::FrameStream;
use crate::test_support::{DenyingAuthZ, EventBrokerHarness, StaticTypesRegistry};

const REQUEST_RESOURCE: &str = "gts.cf.core.events.request.v1~";
const TOPIC: &str = gts_id!("cf.core.events.topic.v1~x.eb.subowner.topic.v1");
const EVENT_TYPE: &str = gts_id!("cf.core.events.event.v1~x.eb.subowner.foo.v1~");
const OTHER_SUBJECT_TYPE: &str = "gts.x.other.v1~";

const JOINER: Uuid = Uuid::from_u128(0x3501);
const TENANT: Uuid = Uuid::from_u128(0x3502);
const OTHER_SUBJECT: Uuid = Uuid::from_u128(0x3503);
const OTHER_TENANT: Uuid = Uuid::from_u128(0x3504);
const INTEREST_TENANT: Uuid = Uuid::from_u128(0x3505);

/// Bounds every stream open: one that never answers fails the test instead
/// of hanging it.
const BOUND: Duration = Duration::from_secs(10);

type Recorded = Arc<Mutex<Vec<Value>>>;

/// The principal that JOINs: a subject of `TENANT` with no subject type.
fn joiner() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(JOINER)
        .subject_tenant_id(TENANT)
        .build()
        .unwrap()
}

/// Another subject of the joiner's tenant.
fn other_principal() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(OTHER_SUBJECT)
        .subject_tenant_id(TENANT)
        .build()
        .unwrap()
}

/// A subscription joined by [`joiner`], with the PDP list cleared after the
/// setup.
struct Joined {
    harness: EventBrokerHarness,
    enforcer: PolicyEnforcer,
    pdp: Recorded,
    group: GtsInstanceId,
    id: Uuid,
}

/// [`joiner`] creates a group and JOINs it with one interest on `TOPIC`
/// under a PDP that allows every request and records each one; `seeded`
/// also seeks partition 0 to earliest, so a stream can open.
async fn joined(seeded: bool) -> Joined {
    let pdp = Recorded::default();
    let sink = Arc::clone(&pdp);
    let enforcer = PolicyEnforcer::new(Arc::new(DenyingAuthZ {
        deny_if: move |req: &EvaluationRequest| {
            sink.lock()
                .unwrap()
                .push(serde_json::to_value(req).unwrap());
            false
        },
    }));
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": TOPIC, "partitions": 1 },
        ])))
        .with_policy_enforcer(enforcer.clone())
        .build()
        .await;
    let group = harness
        .delivery()
        .create_consumer_group(
            &joiner(),
            ConsumerGroupCreateInput {
                client_agent: Some("subscription-owner-test".to_owned()),
                description: None,
            },
        )
        .await
        .expect("the group is created")
        .id;
    let id = harness
        .delivery()
        .join(
            &joiner(),
            JoinRequest {
                consumer_group: group.clone(),
                client_agent: "subscription-owner-test".to_owned(),
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
        .id;
    if seeded {
        harness
            .delivery()
            .seek(&joiner(), id, 1, earliest())
            .await
            .expect("the joiner's seek is allowed");
    }
    pdp.lock().unwrap().clear();
    Joined {
        harness,
        enforcer,
        pdp,
        group,
        id,
    }
}

/// Partition 0 of `TOPIC` at earliest.
fn earliest() -> Vec<SeekTarget> {
    vec![SeekTarget {
        topic: GtsInstanceId::try_new(TOPIC).unwrap(),
        partition: 0,
        value: Position::Earliest,
    }]
}

async fn open(joined: &Joined, ctx: &SecurityContext) -> Result<FrameStream, DomainError> {
    tokio::time::timeout(BOUND, joined.harness.delivery().stream(ctx, joined.id))
        .await
        .expect("the open answers within the bound")
}

fn recorded(recorded: &Recorded) -> Vec<Value> {
    recorded.lock().unwrap().clone()
}

/// The tenant gate's request for `ctx` on the subscription's tenant, as
/// `tenant_authorized` sends it: `TENANT` as the root of a `root_only`
/// context with barriers respected, and as `owner_tenant_id`.
fn gate_request(enforcer: &PolicyEnforcer, ctx: &SecurityContext) -> Vec<Value> {
    vec![
        serde_json::to_value(
            enforcer.build_request_with(
                ctx,
                &ResourceType::from_static(REQUEST_RESOURCE, &[pep_properties::OWNER_TENANT_ID]),
                "consume",
                None,
                false,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, TENANT)
                    .context_tenant_id(TENANT)
                    .tenant_mode(TenantMode::RootOnly)
                    .barrier_mode(PdpBarrierMode::Respect)
                    .require_constraints(false),
            ),
        )
        .unwrap(),
    ]
}

/// The joiner check's refusal of subscription `id`, as the domain returns it.
fn assert_not_owned<T>(result: Result<T, DomainError>, id: Uuid) {
    let Err(error) = result else {
        panic!("expected Forbidden, got Ok");
    };
    let DomainError::Forbidden {
        code,
        message,
        resource,
    } = error
    else {
        panic!("expected Forbidden, got {error:?}");
    };
    assert_eq!(
        (code, message, resource),
        (
            ErrorCode::SubscriptionNotOwned,
            format!("subscription '{id}' was joined by another principal"),
            id.to_string(),
        )
    );
}

/// Neither a stream lease nor a consume scope is held for `joined`, and the
/// joiner can still open its stream.
async fn assert_no_open_left_behind(joined: &Joined) {
    assert_eq!(joined.harness.leases().held_count(), 0);
    assert_eq!(joined.harness.groups().scope_of(joined.id), None);
    open(joined, &joiner())
        .await
        .expect("the joiner's open is allowed");
}

#[tokio::test]
async fn a_leave_by_another_principal_of_the_tenant_is_refused_and_keeps_the_member() {
    let joined = joined(false).await;

    let result = joined
        .harness
        .delivery()
        .leave(&other_principal(), joined.id)
        .await;

    assert_not_owned(result, joined.id);
    assert_eq!(
        recorded(&joined.pdp),
        gate_request(&joined.enforcer, &other_principal())
    );
    let ids: Vec<Uuid> = joined
        .harness
        .groups()
        .list()
        .iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(ids, vec![joined.id]);
}

#[tokio::test]
async fn a_seek_by_another_principal_of_the_tenant_is_refused_and_writes_no_cursor() {
    let joined = joined(false).await;

    let result = joined
        .harness
        .delivery()
        .seek(&other_principal(), joined.id, 1, earliest())
        .await;

    assert_not_owned(result, joined.id);
    assert_eq!(
        recorded(&joined.pdp),
        gate_request(&joined.enforcer, &other_principal())
    );
    let cursor = joined
        .harness
        .repo()
        .find_cursor(&joined.group, &GtsInstanceId::try_new(TOPIC).unwrap(), 0)
        .await
        .expect("reading the cursor must not fail");
    assert!(cursor.is_none(), "the refused seek wrote {cursor:?}");
}

#[tokio::test]
async fn a_stream_open_by_another_principal_of_the_tenant_is_refused_and_takes_no_lease() {
    let joined = joined(true).await;

    let result = open(&joined, &other_principal()).await;

    assert_not_owned(result, joined.id);
    assert_eq!(
        recorded(&joined.pdp),
        gate_request(&joined.enforcer, &other_principal())
    );
    assert_no_open_left_behind(&joined).await;
}

#[tokio::test]
async fn a_stream_open_by_another_principal_before_any_seek_is_refused_not_unseeded() {
    let joined = joined(false).await;

    let result = open(&joined, &other_principal()).await;

    assert_not_owned(result, joined.id);
    assert_eq!(
        recorded(&joined.pdp),
        gate_request(&joined.enforcer, &other_principal())
    );
}

#[tokio::test]
async fn a_seek_by_another_principal_while_the_joiner_streams_is_refused_not_in_progress() {
    let joined = joined(true).await;
    let _stream = open(&joined, &joiner())
        .await
        .expect("the joiner's open is allowed");
    joined.pdp.lock().unwrap().clear();

    let result = joined
        .harness
        .delivery()
        .seek(&other_principal(), joined.id, 1, earliest())
        .await;

    assert_not_owned(result, joined.id);
    assert_eq!(
        recorded(&joined.pdp),
        gate_request(&joined.enforcer, &other_principal())
    );
    assert!(
        joined.harness.leases().is_held(joined.id),
        "the joiner's stream stays open"
    );
}

#[tokio::test]
async fn a_stream_open_by_the_joiners_subject_with_another_subject_type_is_refused() {
    let joined = joined(true).await;
    let typed = SecurityContext::builder()
        .subject_id(JOINER)
        .subject_tenant_id(TENANT)
        .subject_type(OTHER_SUBJECT_TYPE)
        .build()
        .unwrap();

    let result = open(&joined, &typed).await;

    assert_not_owned(result, joined.id);
    assert_eq!(
        recorded(&joined.pdp),
        gate_request(&joined.enforcer, &typed)
    );
    assert_no_open_left_behind(&joined).await;
}

/// The recording PDP allows the tenant gate for any caller tenant, so only
/// the joiner check tells the tenants apart.
#[tokio::test]
async fn a_stream_open_by_the_joiners_subject_in_another_tenant_is_refused() {
    let joined = joined(true).await;
    let elsewhere = SecurityContext::builder()
        .subject_id(JOINER)
        .subject_tenant_id(OTHER_TENANT)
        .build()
        .unwrap();

    let result = open(&joined, &elsewhere).await;

    assert_not_owned(result, joined.id);
    assert_eq!(
        recorded(&joined.pdp),
        gate_request(&joined.enforcer, &elsewhere)
    );
    assert_no_open_left_behind(&joined).await;
}

#[tokio::test]
async fn a_get_by_another_principal_of_the_tenant_answers_the_subscription() {
    let joined = joined(false).await;

    let subscription = joined
        .harness
        .delivery()
        .get_subscription(&other_principal(), joined.id)
        .await
        .expect("a read keeps the tenant gate alone");

    assert_eq!(
        (subscription.id, subscription.tenant_id, subscription.joiner),
        (
            joined.id,
            TENANT,
            Joiner {
                subject: JOINER,
                tenant: TENANT,
                subject_type: None,
            },
        )
    );
    assert_eq!(
        recorded(&joined.pdp),
        gate_request(&joined.enforcer, &other_principal())
    );
}

#[tokio::test]
async fn the_joiner_seeks_streams_and_leaves_its_own_subscription() {
    let joined = joined(false).await;

    joined
        .harness
        .delivery()
        .seek(&joiner(), joined.id, 1, earliest())
        .await
        .expect("the joiner's seek is allowed");
    drop(
        open(&joined, &joiner())
            .await
            .expect("the joiner's open is allowed"),
    );
    joined
        .harness
        .delivery()
        .leave(&joiner(), joined.id)
        .await
        .expect("the joiner's leave is allowed");

    assert!(joined.harness.groups().get(joined.id).is_none());
}

#[tokio::test]
async fn a_leave_by_the_joiner_with_other_token_scopes_is_allowed() {
    let joined = joined(false).await;
    let scoped = SecurityContext::builder()
        .subject_id(JOINER)
        .subject_tenant_id(TENANT)
        .token_scopes(vec!["events:read".to_owned()])
        .build()
        .unwrap();

    joined
        .harness
        .delivery()
        .leave(&scoped, joined.id)
        .await
        .expect("the token scopes are not part of the joiner");

    assert!(joined.harness.groups().get(joined.id).is_none());
}
