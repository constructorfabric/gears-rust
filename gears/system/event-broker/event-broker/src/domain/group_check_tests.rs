//! The group check (#5494): JOIN, stream open and seek ask the PDP for
//! `consume` on the consumer group, decision only. An anonymous group is asked
//! as the group family resource in the group's tenant, a named group as the
//! group itself in the caller's tenant. JOIN asks it in place of the tenant
//! gate on the group's tenant and writes an allow to the memo; a stream open
//! and a seek reuse a live allow and ask only on a miss. A refusal is `403
//! ConsumerGroupNotAuthorized` and leaves no subscription, lease or cursor.
//!
//! Each test records every request the PDP sees after its setup, so the list
//! holds exactly what the call under test asks. The PDP allows every request
//! until a test switches it to refuse the group check.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use authz_resolver_sdk::{
    AccessRequest, BarrierMode as PdpBarrierMode, EvaluationRequest, PolicyEnforcer, ResourceType,
    TenantMode,
};
use chrono::Utc;
use gts::GtsIdPattern;
use serde_json::{Value, json};
use tokio::time::Instant;
use toolkit_gts::GtsInstanceId;
use toolkit_security::{SecurityContext, pep_properties};
use uuid::Uuid;

use super::GroupCheck;
use crate::domain::delivery::{JoinRequest, Position, SeekTarget};
use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::model::{
    BarrierMode, ConsumerGroup, ConsumerGroupCreateInput, ConsumerGroupKind, Cursor, Interest,
    OwnershipFilter, Sequence,
};
use crate::domain::repo::{ConsumerGroupRepo, CursorRepo};
use crate::domain::streaming::scope_memo::{MemoInsert, MemoKey, MemoResult};
use crate::domain::streaming::session::FrameStream;
use crate::test_support::authz_doubles::AllowAllAuthZ;
use crate::test_support::{DenyingAuthZ, EventBrokerHarness, Json, StaticTypesRegistry};

const REQUEST_RESOURCE: &str = "gts.cf.core.events.request.v1~";
const CONSUMER_GROUP_RESOURCE: &str = "gts.cf.core.events.consumer_group.v1~";
const TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.groupcheck.orders.v1";
const EVENT_TYPE: &str = "gts.cf.core.events.event.v1~x.eb.groupcheck.placed.v1~";
const SUBJECT_TYPE: &str = "gts.x.eb.groupcheck.order.v1~";
const NAMED_GROUP: &str =
    "gts.cf.core.events.consumer_group.v1~0190f1c2-7a3b-7c4d-8e5f-6a7b8c9d3704";
const ANONYMOUS_GROUP: &str =
    "gts.cf.core.events.consumer_group.v1~0190f1c2-7a3b-7c4d-8e5f-6a7b8c9d3708";

const SUBJECT: Uuid = Uuid::from_u128(0x3701);
const TENANT: Uuid = Uuid::from_u128(0x3702);
const INTEREST_TENANT: Uuid = Uuid::from_u128(0x3705);
const NAMED_GROUP_TENANT: Uuid = Uuid::from_u128(0x3706);
const NAMED_GROUP_OWNER: Uuid = Uuid::from_u128(0x3707);

/// Bounds every stream open and every await while a stream is held: one that
/// never answers fails the test instead of hanging it.
const BOUND: Duration = Duration::from_secs(10);

type Recorded = Arc<Mutex<Vec<Value>>>;

/// The principal that creates and JOINs the group.
fn caller() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(SUBJECT)
        .subject_tenant_id(TENANT)
        .build()
        .unwrap()
}

/// The caller's anonymous group, under a PDP that records every request and
/// refuses the group check while `refuse` is set; the PDP list is cleared
/// after the setup.
struct Created {
    harness: EventBrokerHarness,
    enforcer: PolicyEnforcer,
    pdp: Recorded,
    refuse: Arc<AtomicBool>,
    group: GtsInstanceId,
}

/// Whether `request` is a group check: `consume` on a consumer-group resource.
fn is_group_check(request: &EvaluationRequest) -> bool {
    request.action.name == "consume"
        && request
            .resource
            .resource_type
            .starts_with(CONSUMER_GROUP_RESOURCE)
}

/// A harness acting as [`caller`] over `TOPIC`, whose PDP records every
/// request and refuses the group check while the returned flag is set.
async fn harness() -> (
    EventBrokerHarness,
    PolicyEnforcer,
    Recorded,
    Arc<AtomicBool>,
) {
    let pdp = Recorded::default();
    let refuse = Arc::new(AtomicBool::new(false));
    let sink = Arc::clone(&pdp);
    let switch = Arc::clone(&refuse);
    let enforcer = PolicyEnforcer::new(Arc::new(DenyingAuthZ {
        deny_if: move |req: &EvaluationRequest| {
            sink.lock()
                .unwrap()
                .push(serde_json::to_value(req).unwrap());
            switch.load(Ordering::SeqCst) && is_group_check(req)
        },
    }));
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": TOPIC, "partitions": 1 },
            { "id": EVENT_TYPE, "topic": TOPIC, "allowed_subject_types": [SUBJECT_TYPE] },
        ])))
        .with_policy_enforcer(enforcer.clone())
        .with_security_context(caller())
        .build()
        .await;
    (harness, enforcer, pdp, refuse)
}

/// [`caller`] creates an anonymous group under [`harness`].
async fn created() -> Created {
    let (harness, enforcer, pdp, refuse) = harness().await;
    let group = harness
        .delivery()
        .create_consumer_group(
            &caller(),
            ConsumerGroupCreateInput {
                client_agent: Some("group-check-test".to_owned()),
                description: None,
            },
        )
        .await
        .expect("the group is created")
        .id;
    pdp.lock().unwrap().clear();
    Created {
        harness,
        enforcer,
        pdp,
        refuse,
        group,
    }
}

/// A JOIN of `group` with one interest on `TOPIC` in `INTEREST_TENANT`.
fn join_request(group: &GtsInstanceId) -> JoinRequest {
    JoinRequest {
        consumer_group: group.clone(),
        client_agent: "group-check-test".to_owned(),
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
    }
}

/// [`caller`] JOINs `created`'s group and, when `seeded`, seeks partition 0
/// to earliest; the PDP list is cleared afterwards. Returns the subscription.
async fn joined(created: &Created, seeded: bool) -> Uuid {
    let id = created
        .harness
        .delivery()
        .join(&caller(), join_request(&created.group))
        .await
        .expect("the JOIN is allowed")
        .subscription
        .id;
    if seeded {
        created
            .harness
            .delivery()
            .seek(&caller(), id, 1, earliest())
            .await
            .expect("the seek is allowed");
    }
    created.pdp.lock().unwrap().clear();
    id
}

/// Partition 0 of `TOPIC` at earliest.
fn earliest() -> Vec<SeekTarget> {
    vec![SeekTarget {
        topic: GtsInstanceId::try_new(TOPIC).unwrap(),
        partition: 0,
        value: Position::Earliest,
    }]
}

async fn open(created: &Created, id: Uuid) -> Result<FrameStream, DomainError> {
    tokio::time::timeout(BOUND, created.harness.delivery().stream(&caller(), id))
        .await
        .expect("the open answers within the bound")
}

fn recorded(recorded: &Recorded) -> Vec<Value> {
    recorded.lock().unwrap().clone()
}

/// The anonymous group check for [`caller`] on `group`: `consume` on the group
/// family resource, which declares `owner_tenant_id`, naming the group and its
/// tenant, with `TENANT` as the root of a `root_only` context with barriers
/// respected.
fn group_check(enforcer: &PolicyEnforcer, group: &GtsInstanceId) -> EvaluationRequest {
    enforcer.build_request_with(
        &caller(),
        &ResourceType::from_static(CONSUMER_GROUP_RESOURCE, &[pep_properties::OWNER_TENANT_ID]),
        "consume",
        None,
        false,
        &AccessRequest::new()
            .resource_property("consumer_group_id", group.as_ref())
            .resource_property(pep_properties::OWNER_TENANT_ID, TENANT)
            .context_tenant_id(TENANT)
            .tenant_mode(TenantMode::RootOnly)
            .barrier_mode(PdpBarrierMode::Respect)
            .require_constraints(false),
    )
}

/// The named group check for [`caller`] and `action` on `NAMED_GROUP`: the
/// group's own id as the resource, which declares `owner_tenant_id`, with the
/// caller's tenant as the root.
fn named_group_check(enforcer: &PolicyEnforcer, action: &'static str) -> Value {
    serde_json::to_value(
        enforcer.build_request_with(
            &caller(),
            &ResourceType::new(NAMED_GROUP.to_owned(), &[pep_properties::OWNER_TENANT_ID]),
            action,
            None,
            false,
            &AccessRequest::new()
                .resource_property("consumer_group_id", NAMED_GROUP)
                .context_tenant_id(TENANT)
                .tenant_mode(TenantMode::RootOnly)
                .barrier_mode(PdpBarrierMode::Respect)
                .require_constraints(false),
        ),
    )
    .unwrap()
}

/// The tenant gate's request for [`caller`] on the subscription's tenant, as
/// `tenant_authorized` sends it.
fn gate(enforcer: &PolicyEnforcer) -> Value {
    serde_json::to_value(
        enforcer.build_request_with(
            &caller(),
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
    .unwrap()
}

/// The consume request (E) of [`join_request`]'s interest.
fn e_request(enforcer: &PolicyEnforcer) -> Value {
    serde_json::to_value(
        enforcer.build_request_with(
            &caller(),
            &ResourceType::from_static(
                TOPIC,
                &[
                    pep_properties::OWNER_TENANT_ID,
                    pep_properties::OWNER_ID,
                    "event_type",
                    "event_subject_type",
                ],
            ),
            "consume",
            None,
            true,
            &AccessRequest::new()
                .context_tenant_id(INTEREST_TENANT)
                .tenant_mode(TenantMode::RootOnly)
                .barrier_mode(PdpBarrierMode::Respect)
                .resource_property("topic_id", TOPIC)
                .resource_property(pep_properties::OWNER_TENANT_ID, INTEREST_TENANT)
                .resource_property("event_type_patterns", json!([EVENT_TYPE]))
                .resource_property("event_types", json!([EVENT_TYPE]))
                .resource_property("event_subject_type_patterns", json!([SUBJECT_TYPE]))
                .require_constraints(true),
        ),
    )
    .unwrap()
}

/// Makes the memo stop answering the group check, as its TTL ending does: an
/// existing key is replaced by an entry that expires at once.
fn expire_group_check(created: &Created) {
    let now = Instant::now();
    let stored = created.harness.scope_memo().insert(
        MemoKey::try_from(&group_check(&created.enforcer, &created.group)).unwrap(),
        MemoResult::Decision(false),
        now,
        now,
    );
    assert_eq!(stored, MemoInsert::Stored);
}

/// The group check's refusal, as the domain returns it.
fn assert_not_authorized<T>(result: Result<T, DomainError>) {
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
            ErrorCode::ConsumerGroupNotAuthorized,
            "access denied by policy".to_owned(),
            String::new(),
        )
    );
}

fn subscription_ids(created: &Created) -> Vec<Uuid> {
    created
        .harness
        .groups()
        .list()
        .iter()
        .map(|s| s.id)
        .collect()
}

/// A JOIN sends one group check, ahead of its consume request, and no tenant
/// gate on the group's tenant.
#[tokio::test]
async fn a_join_of_an_anonymous_group_sends_one_group_check_and_no_tenant_gate() {
    let created = created().await;

    created
        .harness
        .delivery()
        .join(&caller(), join_request(&created.group))
        .await
        .expect("the JOIN is allowed");

    assert_eq!(
        recorded(&created.pdp),
        vec![
            serde_json::to_value(group_check(&created.enforcer, &created.group)).unwrap(),
            e_request(&created.enforcer),
        ]
    );
}

#[tokio::test]
async fn a_join_the_group_check_refuses_is_forbidden_and_sends_no_consume_request() {
    let created = created().await;
    created.refuse.store(true, Ordering::SeqCst);

    let result = created
        .harness
        .delivery()
        .join(&caller(), join_request(&created.group))
        .await;

    assert_not_authorized(result);
    assert_eq!(
        recorded(&created.pdp),
        vec![serde_json::to_value(group_check(&created.enforcer, &created.group)).unwrap()]
    );
    assert_eq!(subscription_ids(&created), Vec::<Uuid>::new());
}

/// Over REST the refusal is the generic permission-denied body naming the
/// consumer-group resource type.
#[tokio::test]
async fn a_join_the_group_check_refuses_answers_403_over_rest() {
    let (harness, _enforcer, _pdp, refuse) = harness().await;
    let created = harness.api_v1().post_consumer_groups().send().await;
    created.assert_status(201);
    let group = created.json()["id"].as_str().unwrap().to_owned();
    refuse.store(true, Ordering::SeqCst);

    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "group-check-test",
            "interests": [{ "topic": TOPIC, "subject_tenant": INTEREST_TENANT, "ownership_filter": "entire_tenant", "types": [EVENT_TYPE] }],
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
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "reason": "access denied by policy",
                "resource_type": CONSUMER_GROUP_RESOURCE,
            },
        })
    );
    assert_eq!(
        harness.groups().list().len(),
        0,
        "no subscription is created"
    );
}

/// A stream open asks the group check after the tenant gate and before the
/// lease; once the JOIN's allow has expired it asks again, and a refusal takes
/// no lease and stores no scope.
#[tokio::test]
async fn a_stream_open_after_the_group_check_is_revoked_is_forbidden_and_takes_no_lease() {
    let created = created().await;
    let id = joined(&created, true).await;
    created.refuse.store(true, Ordering::SeqCst);
    expire_group_check(&created);

    let result = open(&created, id).await;

    assert_not_authorized(result);
    assert_eq!(
        recorded(&created.pdp),
        vec![
            gate(&created.enforcer),
            serde_json::to_value(group_check(&created.enforcer, &created.group)).unwrap(),
        ]
    );
    assert_eq!(created.harness.leases().held_count(), 0);
    assert_eq!(created.harness.groups().scope_of(id), None);

    created.refuse.store(false, Ordering::SeqCst);
    let _stream = open(&created, id).await.expect("the open is allowed again");
    assert_eq!(created.harness.leases().held_count(), 1);
}

/// The group check comes before the cursors: a refused open of a subscription
/// that was never seeked answers the refusal, not `409 PositionsNotSet`.
#[tokio::test]
async fn a_stream_open_the_group_check_refuses_before_any_seek_is_forbidden_not_unseeded() {
    let created = created().await;
    let id = joined(&created, false).await;
    created.refuse.store(true, Ordering::SeqCst);
    expire_group_check(&created);

    let result = open(&created, id).await;

    assert_not_authorized(result);
    assert_eq!(
        recorded(&created.pdp),
        vec![
            gate(&created.enforcer),
            serde_json::to_value(group_check(&created.enforcer, &created.group)).unwrap(),
        ]
    );
    assert_eq!(created.harness.leases().held_count(), 0);
}

/// A seek writes the group's shared cursors, so it asks the group check too; a
/// refusal writes no cursor.
#[tokio::test]
async fn a_seek_after_the_group_check_is_revoked_is_forbidden_and_writes_no_cursor() {
    let created = created().await;
    let id = joined(&created, false).await;
    created.refuse.store(true, Ordering::SeqCst);
    expire_group_check(&created);

    let result = created
        .harness
        .delivery()
        .seek(&caller(), id, 1, earliest())
        .await;

    assert_not_authorized(result);
    assert_eq!(
        recorded(&created.pdp),
        vec![
            gate(&created.enforcer),
            serde_json::to_value(group_check(&created.enforcer, &created.group)).unwrap(),
        ]
    );
    let cursor = created
        .harness
        .repo()
        .find_cursor(&created.group, &GtsInstanceId::try_new(TOPIC).unwrap(), 0)
        .await
        .expect("reading the cursor must not fail");
    assert!(cursor.is_none(), "the refused seek wrote no cursor");
}

/// A seek asks the group check before the open-stream check: once the grant
/// is revoked, a seek while the joiner's stream is open answers the refusal,
/// not `409 StreamingInProgress`, and moves no cursor. The cursor is set to a
/// sentinel after the open, so a seek to earliest that wrote would change it.
#[tokio::test]
async fn a_seek_refused_by_the_group_check_while_a_stream_is_open_is_forbidden_not_in_progress() {
    let created = created().await;
    let id = joined(&created, true).await;
    let _stream = open(&created, id).await.expect("the open is allowed");
    assert_eq!(created.harness.leases().held_count(), 1);
    let topic = GtsInstanceId::try_new(TOPIC).unwrap();
    let sentinel = Cursor {
        topic: topic.clone(),
        consumer_group: created.group.clone(),
        partition: 0,
        offset: Sequence::assigned(7),
    };
    tokio::time::timeout(
        BOUND,
        created
            .harness
            .repo()
            .put_cursors(std::slice::from_ref(&sentinel)),
    )
    .await
    .expect("the cursor write answers within the bound")
    .expect("writing the sentinel cursor must not fail");
    created.pdp.lock().unwrap().clear();
    created.refuse.store(true, Ordering::SeqCst);
    expire_group_check(&created);

    let result = tokio::time::timeout(
        BOUND,
        created
            .harness
            .delivery()
            .seek(&caller(), id, 1, earliest()),
    )
    .await
    .expect("the seek answers within the bound");

    assert_not_authorized(result);
    assert_eq!(
        recorded(&created.pdp),
        vec![
            gate(&created.enforcer),
            serde_json::to_value(group_check(&created.enforcer, &created.group)).unwrap(),
        ]
    );
    let cursor = tokio::time::timeout(
        BOUND,
        created
            .harness
            .repo()
            .find_cursor(&created.group, &topic, 0),
    )
    .await
    .expect("the cursor read answers within the bound")
    .expect("reading the cursor must not fail");
    assert_eq!(
        cursor.map(|c| c.offset),
        Some(Sequence::assigned(7)),
        "the refused seek moved no cursor"
    );
    assert_eq!(created.harness.leases().held_count(), 1);
}

/// The JOIN's allow serves the seek and the open for the scope TTL: neither
/// sends the group check again.
#[tokio::test]
async fn a_stream_open_within_the_ttl_of_the_join_reuses_its_group_check() {
    let created = created().await;
    let id = joined(&created, true).await;

    let _stream = open(&created, id).await.expect("the open is allowed");

    assert_eq!(recorded(&created.pdp), vec![gate(&created.enforcer)]);
    assert_eq!(
        created.harness.scope_memo().get(
            &MemoKey::try_from(&group_check(&created.enforcer, &created.group)).unwrap(),
            Instant::now()
        ),
        Some(MemoResult::Decision(true)),
        "the JOIN left its group-check allow in the memo"
    );
}

/// A named group is asked as the group itself in the caller's tenant, by JOIN
/// (`consume`), get (`consume`) and delete (`manage`). Its id ends in a UUID,
/// so the JOIN can write its owner marker.
#[tokio::test]
async fn a_named_group_is_checked_on_its_own_id_in_the_callers_tenant() {
    let (harness, enforcer, pdp, _refuse) = harness().await;
    let named = GtsInstanceId::try_new(NAMED_GROUP).unwrap();
    harness
        .repo()
        .create_consumer_group(ConsumerGroup {
            id: named.clone(),
            kind: ConsumerGroupKind::Named,
            tenant_id: NAMED_GROUP_TENANT,
            owner_principal_id: NAMED_GROUP_OWNER,
            description: None,
            created_at: Utc::now(),
        })
        .await
        .expect("seeding the named group must not fail");

    let id = harness
        .delivery()
        .join(
            &caller(),
            JoinRequest {
                interests: Vec::new(),
                ..join_request(&named)
            },
        )
        .await
        .expect("the JOIN is allowed")
        .subscription
        .id;
    let joined = recorded(&pdp);
    harness
        .delivery()
        .leave(&caller(), id)
        .await
        .expect("the LEAVE is allowed");
    pdp.lock().unwrap().clear();
    harness
        .delivery()
        .get_consumer_group(&caller(), &named)
        .await
        .expect("the get is allowed");
    let read = recorded(&pdp);
    pdp.lock().unwrap().clear();
    harness
        .delivery()
        .delete_consumer_group(&caller(), &named)
        .await
        .expect("the delete is allowed");
    let deleted = recorded(&pdp);

    assert_eq!(joined, vec![named_group_check(&enforcer, "consume")]);
    assert_eq!(read, vec![named_group_check(&enforcer, "consume")]);
    assert_eq!(deleted, vec![named_group_check(&enforcer, "manage")]);
}

/// A member can outlive its group only through a race with the delete (which
/// refuses a group with members) or a delete in the store; its open then finds
/// no group and answers `404 ConsumerGroupNotFound` before the lease.
#[tokio::test]
async fn a_stream_open_of_a_group_deleted_under_its_subscription_answers_not_found() {
    let created = created().await;
    let id = joined(&created, true).await;
    created
        .harness
        .repo()
        .delete_consumer_group(&created.group)
        .await
        .expect("deleting the group row must not fail");

    let result = open(&created, id).await;

    let Err(DomainError::NotFound {
        code,
        message,
        resource,
    }) = result
    else {
        panic!("expected NotFound, got {:?}", result.map(|_| ()));
    };
    assert_eq!(
        (code, message, resource),
        (
            ErrorCode::ConsumerGroupNotFound,
            format!("consumer group '{}' is not registered", created.group),
            created.group.to_string(),
        )
    );
    assert_eq!(recorded(&created.pdp), vec![gate(&created.enforcer)]);
    assert_eq!(created.harness.leases().held_count(), 0);
}

/// The anonymous group check names the group's tenant as its root and its
/// `owner_tenant_id`, whoever asks. Through JOIN, a stream open or a seek only
/// a caller of the group's tenant reaches it (the owner check and the joiner
/// check come first), so the two tenants are equal there; this builds the
/// check for a caller of another tenant directly.
#[test]
fn an_anonymous_group_check_names_the_groups_tenant_whoever_asks() {
    let enforcer = PolicyEnforcer::new(Arc::new(AllowAllAuthZ));
    let elsewhere = SecurityContext::builder()
        .subject_id(SUBJECT)
        .subject_tenant_id(INTEREST_TENANT)
        .build()
        .unwrap();
    let group = ConsumerGroup {
        id: GtsInstanceId::try_new(ANONYMOUS_GROUP).unwrap(),
        kind: ConsumerGroupKind::Anonymous,
        tenant_id: TENANT,
        owner_principal_id: SUBJECT,
        description: None,
        created_at: Utc::now(),
    };

    let sent =
        GroupCheck::new(&elsewhere, &group, "consume").evaluation_request(&elsewhere, &enforcer);

    assert_eq!(
        serde_json::to_value(sent).unwrap(),
        json!({
            "subject": { "id": SUBJECT, "properties": { "tenant_id": INTEREST_TENANT } },
            "action": { "name": "consume" },
            "resource": {
                "type": CONSUMER_GROUP_RESOURCE,
                "properties": { "consumer_group_id": ANONYMOUS_GROUP, "owner_tenant_id": TENANT },
            },
            "context": {
                "tenant_context": {
                    "mode": "root_only",
                    "root_id": TENANT,
                    "barrier_mode": "respect",
                },
                "token_scopes": [],
                "require_constraints": false,
                "capabilities": [],
                "supported_properties": ["owner_tenant_id"],
            },
        })
    );
}
